// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A fade curve drawn over its duration: a grid with the time along the top and the curve in
/// yellow, rising (a level fading up) or falling (a level fading down), as the engine shapes it.
final class FadeCurveView: NSView {
    let rising: Bool
    var curve = "linear" { didSet { if curve != oldValue { needsDisplay = true } } }
    var duration = 0.0 { didSet { if duration != oldValue { needsDisplay = true } } }

    init(rising: Bool) {
        self.rising = rising
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// The engine's exponent for its logarithmic and exponential shapes.
    private static let expK = 6.907755278982137  // ln(1000)

    /// The engine's curve shapes (`Curve::shape`): how far a rising fade is at `t` of its length.
    static func shape(_ curve: String, _ t: Double) -> Double {
        let t = min(max(t, 0), 1)
        func exp(_ t: Double) -> Double { expm1(expK * t) / expm1(expK) }
        switch curve {
        case "equal_power": return sin(t * .pi / 2)
        case "s_curve": return (1 - cos(t * .pi)) / 2
        case "logarithmic": return 1 - exp(1 - t)
        case "exponential": return exp(t)
        default: return t
        }
    }

    /// The fraction of the way (0 bottom, 1 top) the drawn line is at `t`: a falling fade is the
    /// rising one mirrored, as the engine ramps down.
    func value(at t: Double) -> Double {
        rising ? Self.shape(curve, t) : Self.shape(curve, 1 - t)
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
            let p = NSPoint(x: bounds.width * CGFloat(t), y: 1 + (bounds.height - 2) * CGFloat(value(at: t)))
            if i == 0 { line.move(to: p) } else { line.line(to: p) }
        }
        NSColor.systemYellow.setStroke()
        line.lineWidth = 1.5
        line.stroke()
    }

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .image }
    override func accessibilityLabel() -> String? { loc(rising ? "fade.curve.rising" : "fade.curve.falling") }
}

/// The Fade cue's Curve tab (CT-10, AUD-09), laid out as QLab's: the curve, the duration and the
/// stop flag in a column on the left; the curve drawn for a level going up and for one going down
/// on the right. Only the standard curves (the drawings are not edited).
@MainActor
final class FadeSettingsPanel: NSObject, NSTextFieldDelegate {
    enum Change: Equatable {
        case duration(Double)
        case curve(String)
        case stopTarget(Bool)
        case mode(String)
    }

    /// core's `FadeMode`, in the order of the menu.
    static let modes = ["absolute", "relative"]
    /// How the levels are read; the inspector puts it on the Levels tab, under the levels' buttons.
    let modePopup = NSPopUpButton()

    /// core's `FadeCurve`, in the order of the menu.
    static let curves = ["s_curve", "linear", "equal_power", "logarithmic", "exponential"]

    let view = NSStackView()
    let durationField = NSTextField()
    let curvePopup = NSPopUpButton()
    let stopCheck = NSButton(checkboxWithTitle: loc("fade.stopTarget"), target: nil, action: nil)
    let risingView = FadeCurveView(rising: true)
    let fallingView = FadeCurveView(rising: false)
    var onChange: ((Change) -> Void)?
    /// The duration text was not a time.
    var onInvalid: (() -> Void)?
    private var kind: JSONValue = .null
    var isLocked = false {
        didSet { applyLock() }
    }

    override init() {
        super.init()
        durationField.setAX("fade-duration")
        curvePopup.setAX("fade-curve")
        stopCheck.setAX("fade-stop-target")
        durationField.delegate = self
        durationField.widthAnchor.constraint(equalToConstant: 90).isActive = true
        for key in Self.curves {
            curvePopup.menu?.addItem(NSMenuItem(title: loc("fade.curve.\(key)"), action: nil, keyEquivalent: ""))
        }
        modePopup.setAX("fade-mode", label: loc("fade.mode"))
        modePopup.controlSize = .small
        modePopup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        for key in Self.modes {
            modePopup.menu?.addItem(NSMenuItem(title: loc("fade.mode.\(key)"), action: nil, keyEquivalent: ""))
        }
        modePopup.action = #selector(modeChanged)
        for c in [curvePopup as NSControl, stopCheck, modePopup] { c.target = self }
        curvePopup.action = #selector(curveChanged)
        stopCheck.action = #selector(stopChanged)
        risingView.setAccessibilityIdentifier("fade-curve-rising")
        fallingView.setAccessibilityIdentifier("fade-curve-falling")
        let label = NSTextField(labelWithString: loc("fade.duration"))
        label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        label.textColor = .secondaryLabelColor
        let durationRow = NSStackView(views: [label, durationField])
        durationRow.spacing = 6
        curvePopup.controlSize = .small
        curvePopup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        stopCheck.controlSize = .small
        stopCheck.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        let side = NSStackView(views: [curvePopup, durationRow, spacer, stopCheck])
        side.orientation = .vertical
        side.alignment = .leading
        side.spacing = 10
        side.widthAnchor.constraint(equalToConstant: LevelsPanel.sideWidth).isActive = true
        curvePopup.widthAnchor.constraint(equalTo: side.widthAnchor).isActive = true
        view.orientation = .horizontal
        view.alignment = .top
        view.spacing = 12
        view.distribution = .fill
        for v in [side, risingView, fallingView] as [NSView] {
            view.addArrangedSubview(v)
            v.heightAnchor.constraint(equalTo: view.heightAnchor).isActive = true
        }
        risingView.widthAnchor.constraint(equalTo: fallingView.widthAnchor).isActive = true
        risingView.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        risingView.heightAnchor.constraint(greaterThanOrEqualToConstant: 60).isActive = true
    }

    /// Shows the Fade cue's `kind`. A duration being typed keeps its text.
    func show(kind: JSONValue) {
        self.kind = kind
        if durationField.currentEditor() == nil {
            durationField.stringValue = CueTime.format(kind["duration"]?.doubleValue ?? 0)
        }
        curvePopup.selectItem(at: Self.curves.firstIndex(of: kind["curve"]?.stringValue ?? "") ?? 0)
        for v in [risingView, fallingView] {
            v.curve = kind["curve"]?.stringValue ?? "linear"
            v.duration = kind["duration"]?.doubleValue ?? 0
        }
        stopCheck.state = kind["stop_target_when_done"] == .bool(true) ? .on : .off
        modePopup.selectItem(at: Self.modes.firstIndex(of: kind["mode"]?.stringValue ?? "") ?? 0)
        applyLock()
    }

    private func applyLock() {
        for c in [durationField, curvePopup, stopCheck, modePopup] as [NSControl] { c.isEnabled = !isLocked }
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        control.endTyping(by: selector)
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard !isLocked else { return }
        let shown = CueTime.format(kind["duration"]?.doubleValue ?? 0)
        guard durationField.stringValue != shown else { return }
        guard let seconds = CueTime.parse(durationField.stringValue) else {
            durationField.stringValue = shown
            onInvalid?()
            return
        }
        onChange?(.duration(seconds))
        durationField.stringValue = CueTime.format(seconds)  // what will be stored, until the model answers
    }

    @objc private func curveChanged() {
        let i = curvePopup.indexOfSelectedItem
        guard !isLocked, Self.curves.indices.contains(i) else { return }
        onChange?(.curve(Self.curves[i]))
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
