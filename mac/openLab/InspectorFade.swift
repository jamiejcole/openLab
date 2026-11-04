// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Fade cue's settings beside its levels (CT-10, AUD-09): how long, which curve, and whether
/// the target stops when the fade is done. Only the standard curves; custom curves are not in
/// v0.0.1 (REQUIREMENTS §21).
@MainActor
final class FadeSettingsPanel: NSObject, NSTextFieldDelegate {
    enum Change: Equatable {
        case duration(Double)
        case curve(String)
        case stopTarget(Bool)
    }

    /// core's `FadeCurve`, in the order of the menu.
    static let curves = ["s_curve", "linear", "equal_power", "logarithmic", "exponential"]

    let view = NSStackView()
    let durationField = NSTextField()
    let curvePopup = NSPopUpButton()
    let stopCheck = NSButton(checkboxWithTitle: loc("fade.stopTarget"), target: nil, action: nil)
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
        durationField.widthAnchor.constraint(equalToConstant: 110).isActive = true
        for key in Self.curves {
            curvePopup.menu?.addItem(NSMenuItem(title: loc("fade.curve.\(key)"), action: nil, keyEquivalent: ""))
        }
        for c in [curvePopup as NSControl, stopCheck] { c.target = self }
        curvePopup.action = #selector(curveChanged)
        stopCheck.action = #selector(stopChanged)
        func row(_ title: String, _ control: NSView) -> NSView {
            let label = NSTextField(labelWithString: title)
            label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            label.textColor = .secondaryLabelColor
            let s = NSStackView(views: [label, control])
            s.orientation = .vertical
            s.alignment = .leading
            s.spacing = 2
            return s
        }
        view.orientation = .vertical
        view.alignment = .leading
        view.spacing = 8
        for v in [row(loc("fade.duration"), durationField), row(loc("fade.curve"), curvePopup), stopCheck] {
            view.addArrangedSubview(v)
        }
    }

    /// Shows the Fade cue's `kind`. A duration being typed keeps its text.
    func show(kind: JSONValue) {
        self.kind = kind
        if durationField.currentEditor() == nil {
            durationField.stringValue = CueTime.format(kind["duration"]?.doubleValue ?? 0)
        }
        curvePopup.selectItem(at: Self.curves.firstIndex(of: kind["curve"]?.stringValue ?? "") ?? 0)
        stopCheck.state = kind["stop_target_when_done"] == .bool(true) ? .on : .off
        applyLock()
    }

    private func applyLock() {
        for c in [durationField, curvePopup, stopCheck] as [NSControl] { c.isEnabled = !isLocked }
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

    @objc private func stopChanged() {
        guard !isLocked else { return }
        onChange?(.stopTarget(stopCheck.state == .on))
    }
}
