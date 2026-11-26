// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// One of a Fade cue's two curve graphs over its duration: a grid with the time along the top and
/// the curve in yellow, rising (a level fading up) or falling (a level fading down), as the engine
/// shapes it. A custom or straight-line shape shows its control points: a click on the graph adds
/// one and drags it, a drag moves one, Delete removes the chosen one (QLab 5 manual pp. 198-199).
final class FadeCurveView: NSView {
    enum Phase {
        /// One step of a drag: `began` on the press, then `moved`, then `ended` on the release.
        case began, moved, ended
        /// A change in one go (a point removed).
        case once
    }

    let rising: Bool
    var shape = FadeCurveSpec.Shape() {
        didSet {
            guard shape != oldValue else { return }
            // The chosen point follows its time when the points are replaced from outside (an
            // undo, another client's edit), so Delete never removes whichever point now has its
            // old index. A press or drag here sets the points itself, with `dragging` set.
            if dragging == nil, let c = chosen, oldValue.points.indices.contains(c) {
                let t = oldValue.points[c].t
                chosen = shape.points.firstIndex { abs($0.t - t) < 1e-9 }
            }
            needsDisplay = true
        }
    }
    var duration = 0.0 { didSet { if duration != oldValue { needsDisplay = true } } }
    /// Points can be added, moved and removed (a custom or straight-line shape, not locked).
    var isEditable = false {
        didSet {
            if !isEditable { chosen = nil }
            needsDisplay = true
        }
    }
    /// The point last pressed, which Delete removes.
    private(set) var chosen: Int? { didSet { if chosen != oldValue { needsDisplay = true } } }
    /// The point a drag moves.
    private var dragging: Int?
    /// The new points and how far the edit has got.
    var onEdit: (([FadeCurveSpec.Point], Phase) -> Void)?

    /// How near (points) a press must be to a control point to take it.
    static let grab: CGFloat = 6
    /// The least distance in time between two points, so a drag never puts one on another.
    static let gap = 0.002

    init(rising: Bool) {
        self.rising = rising
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// The fraction of the way (0 bottom, 1 top) the drawn line is at `t`.
    func value(at t: Double) -> Double { FadeCurveSpec.drawn(shape, rising: rising, at: t) }

    /// Where the point `(t, v)` is drawn.
    func location(_ p: FadeCurveSpec.Point) -> NSPoint {
        NSPoint(x: bounds.width * CGFloat(p.t), y: 1 + (bounds.height - 2) * CGFloat(p.v))
    }

    /// The point drawn at `location`, within the graph.
    func point(at location: NSPoint) -> FadeCurveSpec.Point {
        let w = max(bounds.width, 1)
        let h = max(bounds.height - 2, 1)
        return FadeCurveSpec.Point(
            t: min(max(Double(location.x / w), 0), 1), v: min(max(Double((location.y - 1) / h), 0), 1))
    }

    override var acceptsFirstResponder: Bool { isEditable }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        guard isEditable else { return super.mouseDown(with: event) }
        window?.makeFirstResponder(self)
        press(at: convert(event.locationInWindow, from: nil))
    }

    override func mouseDragged(with event: NSEvent) {
        guard dragging != nil else { return }
        drag(to: convert(event.locationInWindow, from: nil))
    }

    override func mouseUp(with event: NSEvent) {
        release()
    }

    /// A press at `location`: takes the point there, else adds one there (a shape holds at most
    /// `FadeCurveSpec.maxPoints`), and starts a drag of it.
    func press(at location: NSPoint) {
        guard isEditable else { return }
        var points = shape.points
        if let near = points.indices.min(by: { distance(points[$0], location) < distance(points[$1], location) }),
            distance(points[near], location) <= Self.grab
        {
            chosen = near
        } else {
            guard points.count < FadeCurveSpec.maxPoints else { return }
            var p = point(at: location)
            p.t = min(max(p.t, Self.gap), 1 - Self.gap)
            guard !points.contains(where: { abs($0.t - p.t) < Self.gap }) else { return }
            let i = points.firstIndex { $0.t > p.t } ?? points.count
            points.insert(p, at: i)
            chosen = i
        }
        dragging = chosen
        shape.points = points
        onEdit?(points, .began)
    }

    /// The point being dragged goes to `location`, kept between its neighbours in time.
    func drag(to location: NSPoint) {
        guard let i = dragging, shape.points.indices.contains(i) else { return }
        var points = shape.points
        var p = point(at: location)
        let lo = (i > 0 ? points[i - 1].t : 0) + Self.gap
        let hi = (i + 1 < points.count ? points[i + 1].t : 1) - Self.gap
        p.t = min(max(p.t, lo), hi)
        guard points[i] != p else { return }
        points[i] = p
        shape.points = points
        onEdit?(points, .moved)
    }

    func release() {
        guard dragging != nil else { return }
        dragging = nil
        needsDisplay = true
        onEdit?(shape.points, .ended)
    }

    override func keyDown(with event: NSEvent) {
        let delete = event.charactersIgnoringModifiers.map { $0 == "\u{7F}" || $0 == "\u{F728}" } ?? false
        guard isEditable, delete, removeChosen() else { return super.keyDown(with: event) }
    }

    /// Removes the chosen point. Returns whether there was one.
    @discardableResult
    func removeChosen() -> Bool {
        guard isEditable, dragging == nil, let i = chosen, shape.points.indices.contains(i) else { return false }
        shape.points.remove(at: i)
        chosen = nil
        onEdit?(shape.points, .once)
        return true
    }

    private func distance(_ p: FadeCurveSpec.Point, _ location: NSPoint) -> CGFloat {
        let at = self.location(p)
        return hypot(at.x - location.x, at.y - location.y)
    }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        NSColor(white: 0.12, alpha: 1).setFill()
        bounds.fill()
        let columns = 10
        let rows = 4
        NSColor(white: 0, alpha: 1).setStroke()
        let grid = NSBezierPath()
        for c in 0...columns {
            let x = (bounds.width * CGFloat(c) / CGFloat(columns)).rounded() + 0.5
            grid.move(to: NSPoint(x: x, y: 0))
            grid.line(to: NSPoint(x: x, y: bounds.height))
        }
        for r in 0...rows {
            let y = (bounds.height * CGFloat(r) / CGFloat(rows)).rounded() + 0.5
            grid.move(to: NSPoint(x: 0, y: y))
            grid.line(to: NSPoint(x: bounds.width, y: y))
        }
        grid.lineWidth = 1
        grid.stroke()
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 9, weight: .regular),
            .foregroundColor: NSColor(white: 0.85, alpha: 1),
        ]
        for c in 0..<columns where bounds.width / CGFloat(columns) >= 28 {
            let seconds = duration * Double(c) / Double(columns)
            String(format: "%.1f", seconds).draw(
                at: NSPoint(x: bounds.width * CGFloat(c) / CGFloat(columns) + 3, y: bounds.height - 14),
                withAttributes: attrs)
        }
        let line = NSBezierPath()
        let steps = max(2, Int(bounds.width))
        for i in 0...steps {
            let t = Double(i) / Double(steps)
            let p = location(FadeCurveSpec.Point(t: t, v: value(at: t)))
            if i == 0 { line.move(to: p) } else { line.line(to: p) }
        }
        NSColor.systemYellow.setStroke()
        line.lineWidth = 1.5
        line.stroke()
        guard shape.kind.hasPoints else { return }
        for (i, p) in shape.points.enumerated() {
            let at = location(p)
            let r: CGFloat = i == chosen ? 4.5 : 3.5
            let dot = NSBezierPath(ovalIn: NSRect(x: at.x - r, y: at.y - r, width: 2 * r, height: 2 * r))
            NSColor.systemYellow.setFill()
            dot.fill()
            if i == chosen {
                NSColor.white.setStroke()
                dot.lineWidth = 1
                dot.stroke()
            }
        }
        // While a point is dragged: where it is, as a share of the change and a time into the fade.
        if let i = dragging, shape.points.indices.contains(i) {
            let p = shape.points[i]
            let text = String(
                format: loc("fade.curve.readout"), String(format: "%.1f", p.v * 100), CueTime.running(p.t * duration))
            let label: [NSAttributedString.Key: Any] = [
                .font: NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .semibold),
                .foregroundColor: NSColor.systemYellow,
            ]
            let size = text.size(withAttributes: label)
            let at = location(p)
            let x = min(max(at.x - size.width / 2, 2), bounds.width - size.width - 2)
            let y = at.y - 8 - size.height >= 2 ? at.y - 8 - size.height : at.y + 8
            text.draw(at: NSPoint(x: x, y: y), withAttributes: label)
        }
    }

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .image }
    override func accessibilityLabel() -> String? { loc(rising ? "fade.curve.rising" : "fade.curve.falling") }
}

/// The Fade cue's Curve tab (CT-10, AUD-09; QLab 5 manual pp. 198-199), laid out as QLab's: a column
/// on the left with the shape, its strength (parametric only), the level scale (audio domain), the
/// duration, the reset of a hand-drawn shape and the stop flag; the curve drawn for a level going up
/// and for one going down on the right, with a lock between that mirrors an edit of either onto the
/// other.
@MainActor
final class FadeSettingsPanel: NSObject, NSTextFieldDelegate {
    enum Change: Equatable {
        case duration(Double)
        /// The whole curve; `gesture` names the drag it belongs to (nil for a one-off change).
        case curve(JSONValue, gesture: Int?)
        case stopTarget(Bool)
        case mode(String)
    }

    /// core's `FadeMode`, in the order of the menu.
    static let modes = ["absolute", "relative"]
    /// How the levels are read; the inspector puts it on the Levels tab, under the levels' buttons.
    let modePopup = NSPopUpButton()

    /// core's standard `FadeCurve`s (an Audio cue's built-in fades), in the order of their menu.
    static let curves = ["s_curve", "linear", "equal_power", "logarithmic", "exponential"]

    let view = NSStackView()
    let durationField = NSTextField()
    /// The shape of both graphs.
    let curvePopup = NSPopUpButton()
    let intensityField = NSTextField()
    private let intensityRow: NSStackView
    let domainPopup = NSPopUpButton()
    let resetButton = NSButton(title: loc("fade.curve.reset"), target: nil, action: nil)
    let stopCheck = NSButton(checkboxWithTitle: loc("fade.stopTarget"), target: nil, action: nil)
    let risingView = FadeCurveView(rising: true)
    let fallingView = FadeCurveView(rising: false)
    /// On: an edit of either graph's points is made to the other too, mirrored.
    let lockButton = NSButton()
    var onChange: ((Change) -> Void)?
    /// The duration or strength text was not one it can be.
    var onInvalid: ((String) -> Void)?
    /// A drag of a point ended: the model may be shown again.
    var onGestureEnd: (() -> Void)?
    private var kind: JSONValue = .null
    /// The curve shown: the model's, or the one a drag in progress has made.
    private(set) var curve = FadeCurveSpec()
    /// The drag in progress, whose edits share an undo step; the model's curve is not shown meanwhile.
    private(set) var gesture: Int?
    private static var gestureSeq = 0
    var isLocked = false {
        didSet { applyLock() }
    }

    override init() {
        let intensityLabel = NSTextField(labelWithString: loc("fade.intensity"))
        intensityRow = NSStackView(views: [intensityLabel, intensityField])
        super.init()
        durationField.setAX("fade-duration")
        curvePopup.setAX("fade-curve", label: loc("fade.shape"))
        domainPopup.setAX("fade-audio-domain", label: loc("fade.domain"))
        intensityField.setAX("fade-curve-intensity", label: loc("fade.intensity"))
        resetButton.setAX("fade-curve-reset")
        lockButton.setAX("fade-curve-lock", label: loc("fade.curve.lock"))
        stopCheck.setAX("fade-stop-target")
        durationField.delegate = self
        intensityField.delegate = self
        durationField.widthAnchor.constraint(equalToConstant: 90).isActive = true
        intensityField.widthAnchor.constraint(equalToConstant: 90).isActive = true
        for key in FadeCurveSpec.Kind.allCases {
            curvePopup.menu?.addItem(
                NSMenuItem(title: loc("fade.shape.\(key.rawValue)"), action: nil, keyEquivalent: ""))
        }
        for key in FadeCurveSpec.Domain.allCases {
            domainPopup.menu?.addItem(
                NSMenuItem(title: loc("fade.domain.\(key.rawValue)"), action: nil, keyEquivalent: ""))
        }
        modePopup.setAX("fade-mode", label: loc("fade.mode"))
        for key in Self.modes {
            modePopup.menu?.addItem(NSMenuItem(title: loc("fade.mode.\(key)"), action: nil, keyEquivalent: ""))
        }
        for p in [modePopup, curvePopup, domainPopup] {
            p.controlSize = .small
            p.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        }
        modePopup.action = #selector(modeChanged)
        for c in [curvePopup as NSControl, domainPopup, stopCheck, modePopup, resetButton] { c.target = self }
        curvePopup.action = #selector(curveChanged)
        domainPopup.action = #selector(domainChanged)
        resetButton.action = #selector(resetClicked)
        stopCheck.action = #selector(stopChanged)
        risingView.setAccessibilityIdentifier("fade-curve-rising")
        fallingView.setAccessibilityIdentifier("fade-curve-falling")
        for v in [risingView, fallingView] {
            v.onEdit = { [weak self, weak v] points, phase in
                guard let self, let v else { return }
                self.pointsEdited(points, phase, rising: v.rising)
            }
        }
        lockButton.setButtonType(.toggle)
        lockButton.isBordered = false
        lockButton.imagePosition = .imageOnly
        lockButton.image = NSImage(systemSymbolName: "lock.open", accessibilityDescription: loc("fade.curve.lock"))
        lockButton.alternateImage = NSImage(systemSymbolName: "lock", accessibilityDescription: loc("fade.curve.lock"))
        lockButton.contentTintColor = .secondaryLabelColor
        lockButton.state = .on
        lockButton.toolTip = loc("fade.curve.lock")
        resetButton.controlSize = .small
        resetButton.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        resetButton.bezelStyle = .rounded

        func label(_ key: String) -> NSTextField {
            let l = NSTextField(labelWithString: loc(key))
            l.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            l.textColor = .secondaryLabelColor
            return l
        }
        intensityLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        intensityLabel.textColor = .secondaryLabelColor
        intensityRow.spacing = 6
        let domainRow = NSStackView(views: [label("fade.domain"), domainPopup])
        domainRow.spacing = 6
        let durationRow = NSStackView(views: [label("fade.duration"), durationField])
        durationRow.spacing = 6
        stopCheck.controlSize = .small
        stopCheck.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        let side = NSStackView(views: [
            curvePopup, intensityRow, domainRow, durationRow, spacer, resetButton, stopCheck,
        ])
        side.orientation = .vertical
        side.alignment = .leading
        side.spacing = 10
        side.widthAnchor.constraint(equalToConstant: LevelsPanel.sideWidth).isActive = true
        curvePopup.widthAnchor.constraint(equalTo: side.widthAnchor).isActive = true
        view.orientation = .horizontal
        view.alignment = .top
        view.spacing = 12
        view.distribution = .fill
        // The lock sits in a narrow gutter between the graphs, half way down.
        let gutter = NSView()
        lockButton.translatesAutoresizingMaskIntoConstraints = false
        gutter.addSubview(lockButton)
        NSLayoutConstraint.activate([
            gutter.widthAnchor.constraint(equalToConstant: 18),
            lockButton.centerXAnchor.constraint(equalTo: gutter.centerXAnchor),
            lockButton.centerYAnchor.constraint(equalTo: gutter.centerYAnchor),
        ])
        for v in [side, risingView, gutter, fallingView] as [NSView] {
            view.addArrangedSubview(v)
            v.heightAnchor.constraint(equalTo: view.heightAnchor).isActive = true
        }
        view.setCustomSpacing(4, after: risingView)
        view.setCustomSpacing(4, after: gutter)
        risingView.widthAnchor.constraint(equalTo: fallingView.widthAnchor).isActive = true
        risingView.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        risingView.heightAnchor.constraint(greaterThanOrEqualToConstant: 60).isActive = true
    }

    /// Whether an edit of one graph's points is made to the other too.
    var mirrors: Bool { lockButton.state == .on }

    /// Shows the Fade cue's `kind`. A duration or strength being typed keeps its text, and the curve a
    /// drag is making stays until the drag ends.
    func show(kind: JSONValue) {
        self.kind = kind
        if durationField.currentEditor() == nil {
            durationField.stringValue = CueTime.precise(kind["duration"]?.doubleValue ?? 0)
        }
        if gesture == nil { curve = FadeCurveSpec(json: kind["curve"]) }
        for v in [risingView, fallingView] { v.duration = kind["duration"]?.doubleValue ?? 0 }
        stopCheck.state = kind["stop_target_when_done"] == .bool(true) ? .on : .off
        modePopup.selectItem(at: Self.modes.firstIndex(of: kind["mode"]?.stringValue ?? "") ?? 0)
        showCurve()
    }

    private func showCurve() {
        curvePopup.selectItem(at: FadeCurveSpec.Kind.allCases.firstIndex(of: curve.rising.kind) ?? 0)
        domainPopup.selectItem(at: FadeCurveSpec.Domain.allCases.firstIndex(of: curve.domain) ?? 0)
        if intensityField.currentEditor() == nil { intensityField.stringValue = Self.number(curve.rising.parameter) }
        intensityRow.isHidden = curve.rising.kind != .parametric
        resetButton.isHidden = !curve.rising.kind.hasPoints
        risingView.shape = curve.rising
        fallingView.shape = curve.falling
        applyLock()
    }

    private static func number(_ x: Double) -> String {
        let f = NumberFormatter()
        f.minimumFractionDigits = 0
        f.maximumFractionDigits = 2
        f.minimumIntegerDigits = 1
        return f.string(from: NSNumber(value: x)) ?? "\(x)"
    }

    private func applyLock() {
        for c in [
            durationField, curvePopup, domainPopup, intensityField, resetButton, stopCheck, modePopup, lockButton,
        ]
            as [NSControl]
        {
            c.isEnabled = !isLocked
        }
        risingView.isEditable = !isLocked && curve.rising.kind.hasPoints
        fallingView.isEditable = !isLocked && curve.falling.kind.hasPoints
    }

    /// Makes `change` to the curve shown and sends the whole curve.
    private func edit(gesture: Int? = nil, _ change: (inout FadeCurveSpec) -> Void) {
        guard !isLocked else { return }
        var c = curve
        change(&c)
        guard c != curve else { return }
        curve = c
        showCurve()
        onChange?(.curve(c.json, gesture: gesture))
    }

    private func pointsEdited(_ points: [FadeCurveSpec.Point], _ phase: FadeCurveView.Phase, rising: Bool) {
        if phase == .began {
            Self.gestureSeq += 1
            gesture = Self.gestureSeq
        }
        let step = phase == .once ? nil : gesture
        edit(gesture: step) { c in
            if rising { c.rising.points = points } else { c.falling.points = points }
            if mirrors {
                if rising {
                    c.falling.points = FadeCurveSpec.mirrored(points)
                } else {
                    c.rising.points = FadeCurveSpec.mirrored(points)
                }
            }
        }
        if phase == .ended {
            gesture = nil
            onGestureEnd?()
        }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        control.endTyping(by: selector)
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard !isLocked else { return }
        if (obj.object as? NSTextField) === intensityField {
            return intensityTyped()
        }
        let shown = CueTime.precise(kind["duration"]?.doubleValue ?? 0)
        guard durationField.stringValue != shown else { return }
        guard let seconds = CueTime.parse(durationField.stringValue) else {
            durationField.stringValue = shown
            onInvalid?(loc("inspector.invalid.time"))
            return
        }
        onChange?(.duration(seconds))
        durationField.stringValue = CueTime.precise(seconds)  // what will be stored, until the model answers
    }

    private func intensityTyped() {
        let text = intensityField.stringValue.trimmingCharacters(in: .whitespaces)
        let shown = Self.number(curve.rising.parameter)
        guard text != shown else { return }
        let f = NumberFormatter()
        f.numberStyle = .decimal
        guard let value = f.number(from: text)?.doubleValue ?? Double(text),
            FadeCurveSpec.parameterRange.contains(value)
        else {
            intensityField.stringValue = shown
            onInvalid?(loc("inspector.invalid.intensity"))
            return
        }
        intensityField.stringValue = Self.number(value)
        edit { c in
            c.rising.parameter = value
            c.falling.parameter = value
        }
    }

    @objc private func curveChanged() {
        let all = FadeCurveSpec.Kind.allCases
        let i = curvePopup.indexOfSelectedItem
        guard !isLocked, all.indices.contains(i) else { return }
        edit { c in
            c.rising.kind = all[i]
            c.falling.kind = all[i]
        }
    }

    @objc private func domainChanged() {
        let all = FadeCurveSpec.Domain.allCases
        let i = domainPopup.indexOfSelectedItem
        guard !isLocked, all.indices.contains(i) else { return }
        edit { $0.domain = all[i] }
    }

    @objc private func resetClicked() {
        edit { c in
            c.rising.points = []
            c.falling.points = []
        }
    }

    @objc private func modeChanged() {
        let i = modePopup.indexOfSelectedItem
        guard !isLocked, Self.modes.indices.contains(i) else { return }
        onChange?(.mode(Self.modes[i]))
    }

    @objc private func stopChanged() {
        guard !isLocked else { return }
        onChange?(.stopTarget(stopCheck.state == .on))
    }
}
