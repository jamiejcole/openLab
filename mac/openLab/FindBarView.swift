// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Find tool (ED-09): it takes the place of the cue-type row. A label, the search field, the
/// number found, previous and next arrows, Select Found and Done. There is no replace.
final class FindBarView: NSView, NSSearchFieldDelegate {
    let field = NSSearchField()
    let countLabel = NSTextField(labelWithString: "")
    let previousButton = NSButton()
    let nextButton = NSButton()
    let selectFoundButton = NSButton(title: loc("find.selectFound"), target: nil, action: nil)
    let doneButton = NSButton(title: loc("find.done"), target: nil, action: nil)

    var onQueryChange: ((String) -> Void)?
    var onStep: ((Bool) -> Void)?
    var onSelectFound: (() -> Void)?
    /// Done, or Esc in the field: clear the search and close the bar.
    var onDone: (() -> Void)?

    var query: String { field.stringValue }

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

    func clear() {
        field.stringValue = ""
        setCount(found: 0, current: nil, searching: false)
    }

    /// "3 of 12" with a current match, "12 found" before one is chosen, nothing for no text yet.
    func setCount(found: Int, current: Int?, searching: Bool) {
        if !searching {
            countLabel.stringValue = ""
        } else if let current {
            countLabel.stringValue = String(format: loc("find.count.of"), current + 1, found)
        } else {
            countLabel.stringValue = String(format: loc("find.count.found"), found)
        }
        for b in [previousButton, nextButton, selectFoundButton] { b.isEnabled = found > 0 }
    }

    func controlTextDidChange(_ obj: Notification) { onQueryChange?(field.stringValue) }

    /// Return steps to the next match; Esc closes the bar. (The
    /// app-wide Esc monitor has already panicked, as it does whatever has the focus.)
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        switch selector {
        case #selector(NSResponder.cancelOperation(_:)):
            onDone?()
            return true
        case #selector(NSResponder.insertNewline(_:)):
            onStep?(true)
            return true
        default:
            return false
        }
    }

    @objc private func previousClicked() { onStep?(false) }
    @objc private func nextClicked() { onStep?(true) }
    @objc private func selectFoundClicked() { onSelectFound?() }
    @objc private func doneClicked() { onDone?() }

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAX(AX.findBar)
        let label = NSTextField(labelWithString: loc("find.label"))
        field.placeholderString = loc("find.placeholder")
        field.delegate = self
        field.sendsSearchStringImmediately = true
        field.setAX(AX.findField, label: loc("find.label"))
        countLabel.textColor = .secondaryLabelColor
        countLabel.setAX(AX.findCount)
        countLabel.setContentHuggingPriority(.required, for: .horizontal)
        countLabel.alignment = .right
        for (button, symbol, key, action) in [
            (previousButton, "chevron.left", "find.previous", #selector(previousClicked)),
            (nextButton, "chevron.right", "find.next", #selector(nextClicked)),
        ] {
            button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: loc(key))
            button.bezelStyle = .texturedRounded
            button.target = self
            button.action = action
            button.toolTip = loc(key)
            button.setAX(key == "find.previous" ? AX.findPrevious : AX.findNext, label: loc(key))
        }
        selectFoundButton.target = self
        selectFoundButton.action = #selector(selectFoundClicked)
        selectFoundButton.bezelStyle = .texturedRounded
        selectFoundButton.setAX(AX.findSelectFound)
        doneButton.target = self
        doneButton.action = #selector(doneClicked)
        doneButton.bezelStyle = .texturedRounded
        doneButton.setAX(AX.findDone)
        setCount(found: 0, current: nil, searching: false)

        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let stack = NSStackView(views: [
            label, field, countLabel, previousButton, nextButton, selectFoundButton, spacer, doneButton,
        ])
        stack.spacing = 8
        field.widthAnchor.constraint(greaterThanOrEqualToConstant: 220).isActive = true
        countLabel.widthAnchor.constraint(greaterThanOrEqualToConstant: 70).isActive = true
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
