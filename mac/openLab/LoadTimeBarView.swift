// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Load to Time tool (QLab manual p. 149): a time field and a scrub slider that take the place of
/// the cue-type row. A time typed (Return) or the slider let go loads the standing-by cue (or its
/// sequence) to that time; a negative time counts back from the end.
final class LoadTimeBarView: NSView, NSTextFieldDelegate {
    let field = NSTextField()
    let slider = NSSlider(value: 0, minValue: 0, maxValue: 1, target: nil, action: nil)
    let doneButton = NSButton(title: loc("loadTime.done"), target: nil, action: nil)

    /// A time was entered or the slider let go (seconds; negative: back from the end).
    var onLoad: ((Double) -> Void)?
    /// Return in the field: the text typed (the window parses it, a negative time included).
    var onSubmit: ((String) -> Void)?
    var onDone: (() -> Void)?

    /// The first responder of the field is its editor while it is being typed in.
    var fieldHasFocus: Bool {
        guard let window else { return false }
        let responder = window.firstResponder
        return responder === field || (responder as? NSText)?.delegate === field
    }

    func focusField() {
        window?.makeFirstResponder(field)
        field.currentEditor()?.selectAll(nil)
    }

    /// The slider spans the standing-by cue's length; none while that is not known.
    func setLength(_ length: Double?) {
        slider.isEnabled = (length ?? 0) > 0
        slider.maxValue = max(length ?? 1, 0.001)
        if slider.doubleValue > slider.maxValue { slider.doubleValue = slider.maxValue }
    }

    /// The slider follows a time that was loaded (a negative one leaves it where it is).
    func show(loaded seconds: Double) {
        if seconds >= 0 { slider.doubleValue = min(seconds, slider.maxValue) }
    }

    func clear() {
        field.stringValue = ""
        slider.doubleValue = 0
    }

    @objc private func sliderLet(_ sender: NSSlider) {
        field.stringValue = CueTime.format(sender.doubleValue)
        onLoad?(sender.doubleValue)
    }

    @objc private func doneClicked() { onDone?() }

    /// Return loads; Esc closes (the app-wide Esc monitor has already panicked, as it does whatever
    /// has the focus).
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.cancelOperation(_:)):
            onDone?()
            return true
        case #selector(NSResponder.insertNewline(_:)):
            onSubmit?(field.stringValue)
            return true
        default:
            return false
        }
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAX(AX.loadBar)
        let label = NSTextField(labelWithString: loc("loadTime.label"))
        field.placeholderString = "00:00.00"
        field.delegate = self
        field.setAX(AX.loadField, label: loc("loadTime.field"))
        field.alignment = .right
        slider.isContinuous = false
        slider.target = self
        slider.action = #selector(sliderLet(_:))
        slider.setAX(AX.loadSlider, label: loc("loadTime.slider"))
        doneButton.target = self
        doneButton.action = #selector(doneClicked)
        doneButton.bezelStyle = .texturedRounded
        doneButton.setAX(AX.loadDone)
        setLength(nil)
        let stack = NSStackView(views: [label, field, slider, doneButton])
        stack.spacing = 8
        field.widthAnchor.constraint(equalToConstant: 100).isActive = true
        slider.widthAnchor.constraint(greaterThanOrEqualToConstant: 200).isActive = true
        slider.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}
